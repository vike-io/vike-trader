//! Property-based tests for `vike_bridge_core::format::format_to_step` — testing-arch plan Phase 6,
//! target P2 (the bridge-core half). This is THE pinned Decimal wire site: every live order's
//! price/qty is quantized here before it hits the venue, and getting the precision or the rounding
//! direction wrong is a `-1111 BAD_PRECISION` reject (or worse, an overshoot past a limit). The
//! module's existing tests pin the exact-rational sibling; these properties assert the four wire
//! LAWS of `format_to_step` itself over random values.
//!
//! Domain: values in the real price range and steps drawn from the actual venue tick/lot grids.
//! Every step in the set is a TERMINATING decimal (`1/step` is exact), so the function's internal
//! `Decimal` division is exact for these inputs — which means every assertion below is an EXACT
//! Decimal comparison with NO numeric tolerance. A failure is therefore a genuine wire bug (a real
//! overshoot / precision leak), never a rounding artifact to be papered over.

use proptest::prelude::*;
use rust_decimal::prelude::*;
use vike_bridge_core::format::{format_to_step, py_f64_str};

/// Digits after the decimal point in a plain decimal string (`"0.00000001"` -> 8, `"10"` -> 0).
fn decimals(s: &str) -> usize {
    s.split_once('.').map(|(_, frac)| frac.len()).unwrap_or(0)
}

/// Realistic positive wire steps, exactly as venues quote them: powers of ten down to the 1e-8
/// crypto floor, plus the non-power-of-ten grids (0.5 / 0.25 / 0.05 / 2.5 / 5 / 0.025) and the
/// Deribit combo 0.0005. Each is a terminating decimal, keeping the internal division exact.
fn step_strategy() -> impl Strategy<Value = &'static str> {
    prop::sample::select(vec![
        "1",
        "10",
        "100",
        "0.1",
        "0.01",
        "0.001",
        "0.0001",
        "0.00001",
        "0.000001",
        "0.0000001",
        "0.00000001",
        "0.5",
        "0.25",
        "0.05",
        "2.5",
        "5",
        "0.0005",
        "0.025",
    ])
}

proptest! {
    /// The four wire laws of `format_to_step` over random values × realistic steps:
    /// (1) ROUND-TRIP — the output parses back as both f64 and Decimal;
    /// (2) PRECISION — it never emits more decimals than the step allows;
    /// (3) STEP-MULTIPLE — it is an exact integer multiple of the step (checked in exact Decimal);
    /// (4) TRUNCATE-TOWARD-ZERO — `|out| <= |value|` (never overshoots a limit), sign preserved.
    #[test]
    fn format_to_step_round_trips_stays_on_grid_and_truncates(
        value in prop_oneof![Just(0.0f64), -1e6f64..-1e-3, 1e-3f64..1e6],
        step in step_strategy(),
    ) {
        let out = format_to_step(value, step);

        // (1) round-trip parse — the output is always a plain (non-exponent) decimal string.
        prop_assert!(out.parse::<f64>().is_ok(), "output {:?} does not parse as f64", out);
        let out_d = Decimal::from_str(&out)
            .unwrap_or_else(|e| panic!("output {:?} does not parse as Decimal: {e}", out));

        // (2) never more precision than the step.
        prop_assert!(
            decimals(&out) <= decimals(step),
            "output {:?} has {} decimals, step {:?} allows only {}",
            out, decimals(&out), step, decimals(step)
        );

        // (3) exact integer multiple of the step.
        let step_d = Decimal::from_str(step).unwrap();
        prop_assert!(
            (out_d % step_d).is_zero(),
            "output {:?} is not a multiple of step {:?} (remainder {})", out, step, out_d % step_d
        );

        // (4) truncation toward zero: |out| <= |value|, with the sign never flipped (a truncation
        // cannot cross zero). Compared against the SAME Decimal image of the value the function uses.
        let value_d = Decimal::from_str(&py_f64_str(value)).unwrap();
        prop_assert!(
            out_d.abs() <= value_d.abs(),
            "output {:?} ({}) overshoots value {} ({}) — not truncated toward zero",
            out, out_d, value, value_d
        );
        if !out_d.is_zero() {
            prop_assert_eq!(
                out_d.is_sign_negative(), value_d.is_sign_negative(),
                "output {:?} sign differs from value {}", out, value
            );
        }
    }
}
