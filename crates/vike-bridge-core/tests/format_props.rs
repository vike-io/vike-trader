//! Property-based tests for `vike_bridge_core::format::format_to_step`, THE pinned Decimal wire
//! site: every live order's price/qty is quantized there before it hits the venue, and getting the
//! precision or the rounding direction wrong is a `-1111 BAD_PRECISION` reject (or worse, an
//! overshoot past a limit). The module's own tests pin the exact-rational sibling; these properties
//! assert the four wire LAWS of `format_to_step` itself over random values.
//!
//! Domain: values in the real price range and steps drawn from the actual venue tick/lot grids.
//! Every step in the set is a TERMINATING decimal (`1/step` is exact), so the function's internal
//! `Decimal` division is exact for these inputs — which means every assertion below is an EXACT
//! Decimal comparison with NO numeric tolerance. A failure is therefore a genuine wire bug (a real
//! overshoot / precision leak), never a rounding artifact to be papered over.

use proptest::prelude::*;
use proptest::test_runner::TestCaseError;
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

/// ⚠ **THE OVERSHOOT BUDGET, and the reason law (4) below has two branches at all.**
///
/// `crates/vike-bridge-core/src/format.rs`'s `grid_multiple_image` emits the EXACT decimal
/// `n · step` when the value it was handed is bit-exactly `n · fl(step)`. For a step whose f64
/// image sits BELOW its decimal (a NECESSARY condition, and a theorem), `n as f64 * step` can land
/// one ULP below the grid point's own float, and `quantize_to_step`'s `trunc()` then eats A WHOLE
/// STEP: a whole tick off a limit price or a whole lot off an order size, on a live order. The
/// rescued decimal is LARGER in magnitude than the value's own shortest round-trip image, so a law
/// `|out| <= |value_image|` and the cure cannot both hold.
///
/// ⚠ **RULED: THE WIRE OWES THE GRID MULTIPLE**, not the value's own image, so law (4)'s two
/// branches are the settled statement of the shipped behaviour. The trade that decided it:
/// emitting the value's own image drops a whole step (**mean 8.45 bps, worst 51.8 bps** over
/// binance spot); emitting the grid multiple overshoots that image by at most **1.314 ULP**,
/// roughly one part in 10^16 and below anything any venue can represent. The per-rung exposure
/// table, the `fl(step)`-vs-decimal sign theorem, the venue census and the overshoot histogram live
/// in `crates/vike-bridge-core/tests/format_props.proptest-regressions`, beside the seed that
/// replays the ruling on every run; they are not restated here.
///
/// ⚠ A ruling is not a tolerance. This constant bounds a WITNESS, the derivation below is what
/// sets it, and neither the ruling nor a future measurement is licence to widen it.
///
/// The number, DERIVED rather than picked. `value = fl(n · fl(step))` carries at most TWO
/// round-to-nearest errors away from the exact grid decimal `n · step`, so
/// `|value − n·step| <= 2u·n·step` with `u = 2^-53`, while `ulp(value) ∈ (u·value, 2u·value]` —
/// **2 ULP**, approached only for a value sitting at the top of its binade. But law (4) does not
/// compare against `value`: it compares against `value`'s SHORTEST ROUND-TRIP IMAGE, which is
/// permitted to sit a further **0.5 ULP** away (that is exactly what "shortest decimal that
/// round-trips" allows). The ceiling on the basis this law actually uses is therefore **2.5 ULP**,
/// and the asserted integer is **3**.
///
/// ⚠ That half-ULP is what keeps the bound true on rungs nobody swept: the record's rung table
/// covers the powers of ten, while the non-power-of-ten grids in [`step_strategy`] (0.25 / 0.05 /
/// 2.5 / 0.0005 / 0.025) are NOT in it, so the ceiling has to come from the theorem.
///
/// MEASURED over 2000 grid multiples at every exposed rung on this law's IMAGE basis: max
/// **1.314 ULP** (at 1e-21; 1.04 over the rungs a venue serves), min 0.500 on every rung. "Never
/// further than ONE ULP" is therefore FALSE and must not be asserted. The strictness lives in the
/// EXACTNESS half of law (4), which admits no tolerance; set against a whole step (at step 1e-6 and
/// value 1e-3, ~2e12 ULP) the gap between 1 and 3 ULP is nothing.
const OVERSHOOT_ULP_BUDGET: u32 = 3;

/// One f64 ULP at `x`'s magnitude: the gap from `|x|` up to the next representable float.
///
/// Spelled from the bit pattern rather than from `f64::next_up` so the file carries no MSRV
/// question; for a finite non-zero `x` well below `f64::MAX` (this suite tops out near 1e16) the
/// successor of `|x|` is `from_bits(to_bits() + 1)` by definition of the IEEE-754 ordering.
fn ulp(x: f64) -> f64 {
    let a = x.abs();
    f64::from_bits(a.to_bits() + 1) - a
}

/// Exact `Decimal` equality that treats every spelling of zero as one value.
///
/// `rust_decimal` carries a sign bit, and `quantize_to_step` deliberately emits a negative zero
/// for a negative value that truncates to nothing (pinned in `format.rs`). That string parses to a
/// zero whose sign flag is set, and the law below must not read it as a different number from the
/// zero it computes. Scale is already irrelevant — `Decimal`'s ordering compares values, so
/// `1.0 == 1.00`.
fn dec_eq(a: Decimal, b: Decimal) -> bool {
    (a.is_zero() && b.is_zero()) || a == b
}

/// THE FOUR WIRE LAWS, as one checkable predicate — `Ok(())`, or the reason it failed.
///
/// Factored out of the `proptest!` block so the SAME law can be driven over deterministic inputs
/// the randomized generator cannot reach (see
/// [`the_laws_hold_in_the_band_where_one_ulp_is_a_whole_unit`]). ⚠ Keeping the generator itself
/// untouched is load-bearing: a committed proptest seed replays an RNG stream through the
/// STRATEGY, so widening the value or step strategy would silently make every recorded
/// counterexample reproduce a DIFFERENT input and invalidate the measurement filed with it.
///
/// (1) ROUND-TRIP — the output parses back as both f64 and Decimal;
/// (2) PRECISION — it never emits more decimals than the step allows;
/// (3) STEP-MULTIPLE — it is an exact integer multiple of the step (checked in exact Decimal);
/// (4) TRUNCATE-TOWARD-ZERO **OR THE GRID-POINT RESCUE** — the output is one of exactly TWO
/// exact decimals and nothing else, and a rescue may move it by only a fraction of a ULP; see
/// [`OVERSHOOT_ULP_BUDGET`] and law (4)'s own comment for what that cannot see.
fn wire_laws(value: f64, step: &str) -> Result<(), String> {
    let out = format_to_step(value, step);

    // (1) round-trip parse — the output is always a plain (non-exponent) decimal string.
    if out.parse::<f64>().is_err() {
        return Err(format!("output {out:?} does not parse as f64"));
    }
    let out_d = match Decimal::from_str(&out) {
        Ok(d) => d,
        Err(e) => return Err(format!("output {out:?} does not parse as Decimal: {e}")),
    };

    // (2) never more precision than the step.
    if decimals(&out) > decimals(step) {
        return Err(format!(
            "output {out:?} has {} decimals, step {step:?} allows only {}",
            decimals(&out),
            decimals(step)
        ));
    }

    // (3) exact integer multiple of the step.
    let step_d =
        Decimal::from_str(step).expect("every step in this suite is a terminating decimal");
    if !(out_d % step_d).is_zero() {
        return Err(format!(
            "output {out:?} is not a multiple of step {step:?} (remainder {})",
            out_d % step_d
        ));
    }

    // The value's own shortest-round-trip decimal image — the SAME image `format_to_step` works
    // from, and the basis law (4) compares against.
    let value_d = Decimal::from_str(&py_f64_str(value))
        .expect("py_f64_str emits a plain decimal for every value in this suite");

    // (4) the sign is never flipped: a truncation cannot cross zero, and a grid-point rescue lands
    // on the multiple the value itself names, so neither branch may change it.
    if !out_d.is_zero() && out_d.is_sign_negative() != value_d.is_sign_negative() {
        return Err(format!("output {out:?} sign differs from value {value}"));
    }

    // (4) ⚠ THE OUTPUT IS ONE OF EXACTLY TWO EXACT DECIMALS, and nothing else is admitted: the
    // exact TRUNCATION of the value's own image, or the exact NEAREST multiple (the grid-point
    // rescue). Both candidates are computed HERE in exact `Decimal`, deliberately NOT as a re-run
    // of the production expression: `grid_multiple_image` picks its `n` as
    // `(value / step_f).round_ties_even()` in f64, and above `2^52`, where one ULP is already a
    // whole unit, the two can DISAGREE — a witness selecting the wrong multiple there moves a live
    // order a full step while every float-side check it could make about itself still passes.
    //
    // ⚠ **WHAT THIS LAW STRUCTURALLY CANNOT SEE.** A witness that selected the multiple one step
    // TOWARD ZERO emits bytes IDENTICAL to plain truncation, a legal answer here: no predicate over
    // `(value, step, out)` alone can separate "the witness picked n-1" from "truncation gave n-1".
    // The COMPOSITIONAL law covers that direction because it knows which `n` was asked for:
    // `crates/vike-bridge-core/tests/grid_multiple_survives_the_wire.rs`. Do not read a green here
    // as covering the step-LOSS direction.
    let Some(ratio) = value_d.checked_div(step_d) else {
        return Err(format!(
            "value/step could not be evaluated in exact Decimal for value {value}, step {step:?} \
             — law (4) cannot be judged"
        ));
    };
    let (Some(truncated), Some(nearest)) =
        (ratio.trunc().checked_mul(step_d), ratio.round().checked_mul(step_d))
    else {
        return Err(format!(
            "the truncated / nearest multiple of step {step:?} overflowed Decimal for value \
             {value} — law (4) cannot be judged"
        ));
    };

    // (4a) THE ORDINARY ANSWER: the exact truncation toward zero. This is also what a value
    // sitting exactly ON a grid point produces, since the two candidates then coincide.
    if dec_eq(out_d, truncated) {
        return Ok(());
    }

    // (4b) THE GRID-POINT RESCUE — the only other admissible answer, and it must be the nearest
    // multiple ON THE NOSE.
    if !dec_eq(out_d, nearest) {
        return Err(format!(
            "output {out:?} ({out_d}) for value {value} ({value_d}) at step {step:?} is NEITHER \
             the exact truncation ({truncated}) NOR the exact nearest multiple ({nearest}). Those \
             are the only two answers `format_to_step` may produce: it quantizes the value's own \
             decimal image, or `grid_multiple_image` recognises a grid point and emits that \
             point's exact decimal. A third answer means the witness selected a multiple the \
             value does not name — see OVERSHOOT_ULP_BUDGET."
        ));
    }

    // (4c) …and a rescue may only move the answer by a fraction of a ULP. A genuinely independent
    // check, because (4b) alone cannot see a rung where a whole step is SMALLER than one ULP, and
    // because a witness that fired on a value nowhere near a grid point would still land on the
    // "nearest" multiple while overshooting by most of a step.
    let Some(budget) = Decimal::from_f64_retain(ulp(value) * f64::from(OVERSHOOT_ULP_BUDGET))
    else {
        return Err(format!(
            "output {out:?} is the nearest multiple rather than the truncation for value {value}, \
             and one ULP at that magnitude ({}) is not representable as a Decimal to bound it with",
            ulp(value)
        ));
    };
    let excess = (out_d.abs() - value_d.abs()).abs();
    if excess > budget {
        return Err(format!(
            "output {out:?} ({out_d}) is the nearest multiple of step {step:?} rather than the \
             truncation, but it sits {excess} from value {value} ({value_d}) — more than \
             {OVERSHOOT_ULP_BUDGET} ULP ({budget}) at that magnitude. A grid-point rescue moves \
             the answer by a fraction of a ULP (measured: 0.500 to 1.314 ULP); this moved it further, \
             so the value was not on the grid and the witness should have declined."
        ));
    }

    Ok(())
}

proptest! {
    /// The four wire laws of `format_to_step` over random values × realistic steps — [`wire_laws`]
    /// is the whole statement, and is shared with the deterministic band case below.
    ///
    /// ⚠ **Neither strategy may be widened.** `format_props.proptest-regressions` (this suite's
    /// persistence path — `WithSource`, the file beside this one, NOT a `proptest-regressions/`
    /// directory: an integration test has no `lib.rs`/`main.rs` above it for proptest's default
    /// `SourceParallel` to find) replays SEEDS, not inputs. A changed strategy re-points every
    /// recorded counterexample at some other value and silently invalidates it.
    #[test]
    fn format_to_step_round_trips_stays_on_grid_and_truncates(
        value in prop_oneof![Just(0.0f64), -1e6f64..-1e-3, 1e-3f64..1e6],
        step in step_strategy(),
    ) {
        if let Err(why) = wire_laws(value, step) {
            return Err(TestCaseError::fail(why));
        }
    }
}

/// ⚠ THE BAND THE RANDOMIZED LAW CANNOT REACH: magnitudes at and above `2^52`, where one f64 ULP
/// is already a whole unit and "which multiple of the step is this" stops having a unique f64
/// answer.
///
/// Driven deterministically rather than folded into the generator above, for the reason that
/// generator's doc gives — widening a strategy invalidates every committed seed — and present
/// because this is precisely where a float-side witness and an exact-decimal one can DISAGREE.
/// `format.rs`'s `grid_multiple_image` declines above a multiple count of `2^52`
/// (`MAX_UNAMBIGUOUS_MULTIPLE`) for exactly that reason; this pins that the laws hold across the
/// whole band either way, so raising or removing that guard reddens something instead of shipping
/// quietly.
///
/// The magnitudes bracket the interesting edges: `2^52` (the first at which a ULP reaches 1.0),
/// `2^53` (`MAX_EXACT_INT`) and the binade above it, each also nudged off the power of two so the
/// cases are not all exactly representable multiples of every step. Both signs, because the
/// direction a wrong multiple moves a value was measured to vary and is not assumed here.
#[test]
fn the_laws_hold_in_the_band_where_one_ulp_is_a_whole_unit() {
    const POW52: f64 = 4_503_599_627_370_496.0; // 2^52 — one ULP is 1.0 from here up
    const POW53: f64 = 9_007_199_254_740_992.0; // 2^53 — format.rs's MAX_EXACT_INT

    let values = [
        POW52,
        POW52 + 1.0,
        POW52 + 3.0,
        POW52 * 1.5,
        POW53 - 2.0,
        POW53,
        POW53 + 2.0,
        POW53 * 1.5,
        -(POW52 + 1.0),
        -POW53,
    ];

    let mut failures = Vec::new();
    for value in values {
        for step in ["1", "10", "100", "0.5", "2.5", "5", "0.25", "0.05", "0.025"] {
            if let Err(why) = wire_laws(value, step) {
                failures.push(format!("value {value} step {step:?}: {why}"));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "the wire laws failed in the 2^52..2^54 band — where one f64 ULP is a whole unit, so a \
         witness selecting the wrong multiple moves a live order a FULL STEP while every \
         float-side check still passes:\n  {}",
        failures.join("\n  ")
    );
}
