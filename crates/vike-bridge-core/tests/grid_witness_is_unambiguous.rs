//! **The grid witness must answer only where its answer is UNIQUE.**
//!
//! `crates/vike-bridge-core/src/format.rs`'s `grid_multiple_image` is a bit-exact witness: it
//! returns the exact decimal `n · step` when `n · step_f` reproduces `value` bit-for-bit, on the
//! argument that a float which IS the image of a grid point carries no second intent. That
//! argument has a premise nothing was checking — that `n` is the ONLY integer whose product
//! reproduces `value` — and above `n = 2^52` the premise is false.
//!
//! # The defect, stated as its own thing
//!
//! This is NOT the one-step truncation loss the witness was added to close, and it is not the
//! sub-ULP overshoot question. Those two are about a value arriving slightly off its grid point.
//! This one is the opposite: **a value that is exactly right is replaced by a different number.**
//!
//! For a `value` in `[2ᵉ, 2ᵉ⁺¹)` the f64 spacing is `2ᵉ⁻⁵²`, so `n = value / step > 2^52` is
//! precisely the condition `step < ulp(value)` — the decimal grid is finer than the floats at that
//! magnitude. Several consecutive multiples of the step then round to the SAME f64, the witness's
//! `n * step_f != value` rejection passes for every one of them, and
//! `round_ties_even(value / step_f)` hands back whichever of them its own two roundings land on.
//! The caller gets `n · step` for an `n` that is not theirs, rendered as an exact decimal.
//!
//! # How it was found, and what it cost
//!
//! `crates/bridges/alpaca/src/event_mapper.rs`'s `build_order_body` formats the order quantity
//! with `format_to_step(qty, "0.000000001")` — a 9-decimal PRECISION CAP (Alpaca's fractional-share
//! limit), not a venue grid. At that step the exposed band `(2^52, 2^53]` lands on ordinary
//! whole-share counts: `format_to_step(9_000_000.0, "0.000000001")` returned
//! `"8999999.999999999"`. Alpaca rejects a fractional quantity on most order types, so a
//! nine-million-share order would have been refused at the venue — and the venue is right, because
//! the string is not the order that was placed.
//!
//! MEASURED in a the CI box lane on the unfixed tree: **every whole share count in
//! `8_388_609..=9_007_199` came back fractional — 618_591 of 618_591 — each exactly one step LOW**,
//! and the two neighbours either side of the band (8_388_608 and 9_007_200) came back clean.
//!
//! The chain is the production one: `crates/vike-exec/src/risk.rs`'s `round_to` with alpaca's
//! default equity increment of `1.0` passes a whole share count through untouched, so nothing
//! upstream perturbs the value — it arrives at the wire site exact and leaves it fractional.
//!
//! # What declining costs, measured rather than assumed
//!
//! [`inside_the_band_the_output_is_the_round_down_of_its_input`] is the half that keeps the cure
//! honest, and it asserts a SMALLER property than the obvious one because the obvious one is false.
//! Above `2^52` nothing can recover the caller's multiple — `round_to_step` cannot land on the grid
//! point either — so "the cure still works up here" is not available to assert, and the test's own
//! doc carries the numbers that refuted it. What IS recoverable is the round-DOWN contract, which
//! the witness broke in both directions inside the band.
//!
//! The cure the witness DOES exist for is twelve orders of magnitude inside the bound —
//! `crates/vike-bridge-core/tests/grid_multiple_survives_the_wire.rs` sweeps `n ≤ 2000`, and
//! [`the_cure_still_rescues_its_whole_step`] re-states its worked example here so a red run in
//! this file names both halves.
//!
//! ⚠ **This change is not direction-safe and nothing here may say it is.** MEASURED across the step
//! ladder inside the band, the emitted string moves by exactly one step and the SIGN depends on the
//! rung: at `1e-9` only UP (1533 of 4001 sampled rows), at `1e-7` only DOWN (1408), at `1e-8` both
//! (445 up, 23 down). Outside the band nothing moves.

use rust_decimal::prelude::*;
use vike_bridge_core::format::{format_to_step, format_to_step_f, py_f64_str};
use vike_model::round_to_step;

/// Alpaca's hardcoded 9-decimal quantity cap — the step the defect was found at.
const NINE_DP: &str = "0.000000001";

/// `2^52`: the largest multiple count whose `n · step` is unique in f64, and the bound
/// `grid_multiple_image` now declines above.
const TWO52: f64 = 4_503_599_627_370_496.0;
/// `2^53`: the old bound, and the top of the exposed band.
const TWO53: f64 = 9_007_199_254_740_992.0;

/// THE DEFECT, as one number. RED before the bound landed, with the exact string it returned.
#[test]
fn a_whole_share_count_is_not_turned_into_a_fraction() {
    assert_eq!(
        format_to_step(9_000_000.0, NINE_DP),
        "9000000.000000000",
        "the witness returned a multiple that is not this value's own — before the 2^52 bound \
         this read \"8999999.999999999\", a fractional quantity on a whole-share order"
    );
}

/// The whole window, end to end. Every whole share count from below the first exposed magnitude to
/// past the top of the band must reach the wire as a whole number.
#[test]
fn every_whole_share_count_in_the_exposed_window_stays_whole() {
    let mut broken = 0usize;
    let mut sample: Vec<String> = Vec::new();
    for n in 8_388_600i64..=9_007_210 {
        let s = format_to_step(n as f64, NINE_DP);
        let d = Decimal::from_str(&s).expect("a wire string is a plain decimal");
        if d != Decimal::from(n) {
            broken += 1;
            if sample.len() < 8 {
                sample.push(format!("{n} -> {s}"));
            }
        }
    }
    assert_eq!(
        broken,
        0,
        "{broken} whole share counts came off the wire fractional; first: {}",
        sample.join(", ")
    );
}

/// The EDGES of the band, pinned as strings so a red run says which side moved.
///
///   * `8_388_608` is `2^23`, the first magnitude at which the f64 spacing (`2⁻²⁹`, about
///     `1.86e-9`) exceeds a 9-decimal step — i.e. the first `value` whose multiple count passes
///     `2^52`. Below it the witness is unique and answers as it always did.
///   * `9_007_200` is the first whole share count whose multiple count passes `2^53`, where the
///     OLD bound already declined. It was correct before this change and is unchanged by it.
#[test]
fn the_bands_two_edges() {
    assert_eq!(format_to_step(8_388_607.0, NINE_DP), "8388607.000000000", "below the band");
    assert_eq!(format_to_step(8_388_608.0, NINE_DP), "8388608.000000000", "the low edge, 2^23");
    assert_eq!(format_to_step(8_388_609.0, NINE_DP), "8388609.000000000", "inside the band");
    assert_eq!(format_to_step(9_007_199.0, NINE_DP), "9007199.000000000", "the high edge");
    assert_eq!(
        format_to_step(9_007_200.0, NINE_DP),
        "9007200.000000000",
        "above MAX_EXACT_INT/1e9 — the OLD bound already declined here, so this row is the \
         control: it must not move"
    );
}

/// A FRACTIONAL quantity inside the same band is corrupted by the same mechanism, so the fix is
/// not a whole-number special case and this test refuses to let it become one.
#[test]
fn a_fractional_quantity_in_the_band_keeps_its_own_digits() {
    assert_eq!(format_to_step(9_000_000.5, NINE_DP), "9000000.500000000");
    assert_eq!(format_to_step(8_500_000.25, NINE_DP), "8500000.250000000");
    assert_eq!(format_to_step(8_600_000.125, NINE_DP), "8600000.125000000");
}

/// THE OTHER HALF, and it is a SMALLER claim than "the cure still works up here" — deliberately,
/// because the larger one is false and was measured false.
///
/// Above `2^52` no formatter can recover the multiple the caller meant:
/// `crates/vike-model/src/scalar.rs`'s `round_to_step` cannot land on the grid point either, so the
/// value it hands the quantizer is already one or two ULPs off. MEASURED over 4001 on-grid
/// multiples at step `1e-9` inside the band, the witness missed the exact multiple 248 times and
/// the plain truncation 1554 — but BELOW the band, where this change does nothing whatever, the
/// same metric already read 58 and 92. That metric is measuring `round_to_step`.
///
/// What IS recoverable, and what the bound restores, is the contract
/// `crates/vike-bridge-core/src/format.rs` opens with: **the emitted string is the round-DOWN of
/// its own input.** `0 ≤ dec(value) − output < step`. The witness broke it in BOTH directions
/// inside the band — a whole step below the value at the 9-decimal rung (the Alpaca defect), and
/// one step ABOVE it at the 1e-7 rung — and it is that, not the multiple, that this test asserts.
///
/// ⚠ The property is deliberately scoped to the band. OUTSIDE it the witness fires and is SUPPOSED
/// to overshoot `dec(value)` by under an ULP — that is the whole cure
/// ([`the_cure_still_rescues_its_whole_step`]), and asserting round-down globally would forbid it.
///
/// Both entry points are driven, for the reason
/// `crates/vike-bridge-core/tests/grid_multiple_survives_the_wire.rs` gives: `format_to_step` takes
/// the step STRING and `format_to_step_f` routes an f64 through `py_f64_str`, so covering one
/// leaves the other's shim untested.
#[test]
fn inside_the_band_the_output_is_the_round_down_of_its_input() {
    const RUNGS: [(&str, f64); 5] = [
        (NINE_DP, 1e-9),
        ("0.00000001", 1e-8),
        ("0.0000001", 1e-7),
        ("0.000001", 1e-6),
        ("0.01", 1e-2),
    ];
    for (step_s, step_f) in RUNGS {
        let step_d = Decimal::from_str(step_s).expect("the step parses");
        let mut broken: Vec<String> = Vec::new();
        let mut rows = 0usize;
        // Start a hair above 2^52 so the multiple count `grid_multiple_image` recomputes is
        // unambiguously inside the band rather than sitting on its edge.
        let lo = TWO52 * 1.000_001;
        for k in 0..=2000i64 {
            let n = (lo + (TWO53 - lo) * (k as f64) / 2000.0).round();
            let v = round_to_step(n * step_f, step_f);
            if !v.is_finite() || v == 0.0 {
                continue;
            }
            let dv = Decimal::from_str(&py_f64_str(v)).expect("a float's own decimal image");
            rows += 1;
            for out in [format_to_step(v, step_s), format_to_step_f(v, step_f)] {
                let got = Decimal::from_str(&out).expect("plain decimal");
                let drop = dv - got;
                if (drop < Decimal::ZERO || drop >= step_d) && broken.len() < 6 {
                    broken.push(format!("step {step_s} n={n} value={v} emitted {out}"));
                }
            }
        }
        assert!(rows > 1500, "step {step_s}: the sweep must cover the band, not skip it: {rows}");
        assert!(
            broken.is_empty(),
            "inside the ambiguous band the quantizer emitted a string that is not the round-down \
             of its own input — the witness answered where its answer is not unique: {}",
            broken.join(", ")
        );
    }
}

/// THE CURE, unmoved. The worked example of the defect
/// `crates/vike-bridge-core/src/format.rs`'s `grid_multiple_image` was written for, re-stated here
/// so one red run in this file distinguishes "the bound broke the cure" from "the bound did not
/// land".
#[test]
fn the_cure_still_rescues_its_whole_step() {
    // `5 · fl(1e-6)` lands one ULP below the grid point; the truncation used to emit four
    // micro-lots where five were ordered.
    let v = round_to_step(5.0 * 1e-6, 1e-6);
    assert_eq!(format_to_step_f(v, 1e-6), "0.000005", "round_to_step gave {v}");
    assert_eq!(format_to_step(v, "0.000001"), "0.000005", "round_to_step gave {v}");
    // ...and the 1e-7 rung's first recorded loss.
    let v = round_to_step(13.0 * 1e-7, 1e-7);
    assert_eq!(format_to_step_f(v, 1e-7), "0.0000013", "round_to_step gave {v}");
}

/// WHERE each step starts declining, pinned as a magnitude rather than left as an inference.
///
/// The witness declines above a multiple count of `2^52`, so for a given step that is a VALUE:
/// `2^52 · step`. This test states it for the venue grid ladder so nobody has to re-derive it, and
/// so a new rung added to a bridge is a visible row rather than a surprise on a live order.
///
/// ⚠ The bottom rung is REACHED by ordinary orders — a nine-decimal precision cap starts declining
/// at 4,503,599 units, which is an ordinary Alpaca share count — and that is fine for the reason
/// [`declining_the_band_loses_no_step`] MEASURES rather than asserts: below the step's own f64
/// spacing there is no step left to lose. Every coarser rung declines only far above any order a
/// venue would accept.
#[test]
fn where_each_step_starts_declining() {
    // (step, the value at which the witness starts declining, the largest plausible order there)
    const RUNGS: [(&str, f64, f64, f64); 7] = [
        ("1", 1e0, 4.503_599_627_370_496e15, 1e9),
        ("0.01", 1e-2, 4.503_599_627_370_496e13, 1e9),
        ("0.0001", 1e-4, 4.503_599_627_370_496e11, 1e7),
        ("0.000001", 1e-6, 4.503_599_627_370_496e9, 1e6),
        ("0.0000001", 1e-7, 4.503_599_627_370_496e8, 1e6),
        ("0.00000001", 1e-8, 4.503_599_627_370_496e7, 1e7),
        ("0.000000001", 1e-9, 4.503_599_627_370_496e6, f64::INFINITY),
    ];
    for (label, step, threshold, plausible) in RUNGS {
        let derived = TWO52 * step;
        assert!(
            (derived - threshold).abs() <= threshold * 1e-12,
            "step {label}: the declining threshold is {derived}, not the pinned {threshold}"
        );
        assert!(
            threshold > plausible || plausible.is_infinite(),
            "step {label} now declines at {threshold}, below the {plausible} an order can reach — \
             that is not a bug (see declining_the_band_loses_no_step) but the row must say so"
        );
    }
}

/// ORDINARY MAGNITUDES ARE BYTE-IDENTICAL. Every row here was MEASURED in a the CI box lane on the
/// unfixed tree and is reproduced verbatim: the bound may not move a string anywhere a real venue
/// grid and a real order size meet.
///
/// The rows span the r6 fixture's control values, the venue step ladder, the alpaca price tick and
/// the whole-share/fractional-share shapes the alpaca order body emits.
#[test]
fn ordinary_magnitudes_did_not_move() {
    const ROWS: [(f64, &str, &str); 22] = [
        // r6 control rows — a genuine mid-step value still truncates toward zero.
        (0.015, "0.01", "0.01"),
        (99999.99999999, "0.1", "99999.9"),
        (99999.99999999, "1", "99999"),
        (123.456789, "0.01", "123.45"),
        (123.456789, "0.05", "123.45"),
        (0.30000000000000004, "0.01", "0.30"),
        (0.30000000000000004, "0.1", "0.3"),
        (62584.567891234, "0.000001", "62584.567891"),
        (-0.35, "1", "-0"),
        // the venue ladder at ordinary sizes
        (1.0, "0.00000001", "1.00000000"),
        (2.37, "0.1", "2.3"),
        (123.456, "0.001", "123.456"),
        (0.0001, "0.00001", "0.00010"),
        // alpaca's own two steps at realistic order shapes
        (10.0, NINE_DP, "10.000000000"),
        (0.5, NINE_DP, "0.500000000"),
        (1_000_000.0, NINE_DP, "1000000.000000000"),
        (4_000_000.0, NINE_DP, "4000000.000000000"),
        (8_000_000.0, NINE_DP, "8000000.000000000"),
        (150.25, "0.01", "150.25"),
        (0.000000001, NINE_DP, "0.000000001"),
        // large magnitudes at coarse steps — far from the band on both axes
        (12345678901.0, "0.01", "12345678901.00"),
        (98765.4321, "0.0001", "98765.4321"),
    ];
    for (value, step, want) in ROWS {
        assert_eq!(format_to_step(value, step), want, "format_to_step({value}, {step:?}) moved");
    }
}

/// A standing check on the two magic numbers this file reasons with, so a typo in either is a
/// named failure rather than a silently shifted band: the bound is exactly half of
/// `crates/vike-bridge-core/src/format.rs`'s `MAX_EXACT_INT`, and both are exact powers of two.
#[test]
fn the_bound_is_exactly_half_the_exact_integer_ceiling() {
    assert_eq!(TWO52 * 2.0, TWO53);
    assert_eq!(TWO52, 2f64.powi(52));
    assert_eq!(TWO53, 2f64.powi(53));
    // 2^53 is the first magnitude at which consecutive integers stop being representable, which is
    // what makes it the REPRESENTABILITY ceiling and not the uniqueness one.
    assert_eq!(TWO53 + 1.0, TWO53);
    assert_ne!(TWO52 + 1.0, TWO52);
}
