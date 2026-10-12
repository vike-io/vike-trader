//! Order qty/price as DECIMAL STRINGS quantized to stepSize/tickSize: the primary pinned Decimal
//! wire site. The core stays `f64`; `rust_decimal` appears only where an exact decimal is what goes
//! on the wire. `crates/vike-bridge-core/tests/wire_quantizer_probe.rs`'s `WIRE_FILES` is the
//! roster of such sites (this file and its declared twins), and its source gate scans each one.
//!
//! RiskGate rounds to tick/lot but leaves IEEE artifacts (0.30000000000000004) that trigger
//! Binance -1111 BAD_PRECISION. Quantize DOWN (never overshoot a limit) to the step's decimal
//! places and emit a plain (non-exponent) string.
//!
//! ⚠ **Quantizing DOWN is also an AMPLIFIER.** The truncation is `(value / step).trunc()`, so a
//! value ONE ULP BELOW a grid point emits a whole step less ([`format_scaled_to_step_f`] states the
//! non-dyadic-rational case). A last-bit disagreement anywhere upstream does not stay a last bit
//! here: it becomes a different tick or lot on a live order, which is why the probe above exists.
//!
//! ⚠ **"Never overshoot" has exactly ONE sanctioned exception, and it was RULED ON**:
//! [`grid_multiple_image`] emits a decimal fractionally ABOVE the value's own image when that value
//! is bit-exactly the f64 image of a grid point. Read that function's doc before reasoning from
//! this header's rule.

use rust_decimal::prelude::*;

/// Shortest round-trip digits that always keep a decimal point for an integral float ("1.0" where
/// Rust prints "1"), Python's `str(float)` spelling that the frozen `fixtures/r6` rows carry. That
/// trailing zero survives into the Decimal's scale, visible on the no-step passthrough.
pub fn py_f64_str(x: f64) -> String {
    let s = format!("{x}");
    if x.is_finite() && !s.contains(['.', 'e', 'E']) { format!("{s}.0") } else { s }
}

fn dec_from_f64(x: f64) -> Option<Decimal> {
    let s = py_f64_str(x);
    Decimal::from_str(&s).ok().or_else(|| Decimal::from_scientific(&s).ok())
}

fn dec_from_step(step: &str) -> Option<Decimal> {
    Decimal::from_str(step).ok().or_else(|| Decimal::from_scientific(step).ok())
}

/// `2^53`, the largest f64 that is still an exact integer count: above it consecutive integers are
/// not representable, so "which multiple of the step is this" has no f64 answer at all.
///
/// ⚠ NOT where [`grid_multiple_image`] declines: that is [`MAX_UNAMBIGUOUS_MULTIPLE`], a binade
/// lower. This one is the derivation base and the twin of the same-named constant in
/// `crates/bridges/hyperliquid/src/px.rs`.
const MAX_EXACT_INT: f64 = 9_007_199_254_740_992.0;

/// The largest multiple count [`grid_multiple_image`] answers for: `2^52`, HALF of
/// [`MAX_EXACT_INT`]. That one is where an `n` stops being REPRESENTABLE; this is where an `n`
/// stops being UNIQUE.
///
/// ⚠ **`n · step_f == value` is a witness only while it has ONE solution.** For a `value` in
/// `[2ᵉ, 2ᵉ⁺¹)` the f64 spacing is `2ᵉ⁻⁵²`, so `n = value / step > 2^52` is exactly the condition
/// `step < ulp(value)`: the grid is finer than the floats, several consecutive multiples round to
/// the same f64, the bit-exact re-multiplication passes for every one of them, and
/// `round_ties_even(value / step_f)` returns whichever its own two roundings land on. A CORRECT
/// value is corrupted into a wrong one: at the 9-decimal cap Alpaca's order body hardcodes
/// (`crates/bridges/alpaca/src/event_mapper.rs`'s `build_order_body`),
/// `format_to_step(9_000_000.0, "0.000000001")` returned `"8999999.999999999"`, and every whole
/// share count in `8_388_609..=9_007_199` came back one step low.
/// `crates/vike-bridge-core/tests/grid_witness_is_unambiguous.rs` holds the sweep and the edges.
///
/// Declining restores the ROUND-DOWN contract this file opens with (the output is the round-down
/// of its INPUT), not the caller's multiple: above `2^52`
/// `crates/vike-model/src/scalar.rs`'s `round_to_step` cannot land on the grid point either. ⚠ It
/// is NOT direction-safe: inside the band the emitted string moves by one step and the sign depends
/// on the rung (measured: only up at `1e-9`, only down at `1e-7`, both at `1e-8`). The cure the
/// witness exists for lives at an `n` in the low thousands
/// (`crates/vike-bridge-core/tests/grid_multiple_survives_the_wire.rs` sweeps `n ≤ 2000`).
///
/// ⚠ The hyperliquid twin (`crates/bridges/hyperliquid/src/px.rs`'s `grid_multiple_image`) is still
/// bounded at its own `2^53`: a declared divergence, not a silent one.
const MAX_UNAMBIGUOUS_MULTIPLE: f64 = MAX_EXACT_INT / 2.0;

/// ⚠ A WITNESS, not a tolerance. Returns the EXACT decimal `n · step` when `value` is
/// BIT-EXACTLY the f64 image of an integer multiple of `step`; `None` otherwise, and the
/// caller then quantizes the value's own decimal image exactly as before.
///
/// # ⚠ The overshoot is DELIBERATE and was RULED ON
///
/// The decimal this returns is LARGER in magnitude than the value's own shortest-round-trip image
/// (the value arrived one f64 ULP BELOW the grid point), against the header's "never overshoot".
/// **The owner ruled that the wire owes the GRID MULTIPLE the caller asked for, not the value's own
/// image.** Removing this to restore the stricter reading reopens a settled question. The verdict,
/// the trade and the pointer to every measurement live at
/// `crates/vike-bridge-core/tests/format_props.rs`'s `OVERSHOOT_ULP_BUDGET`, which is also the law
/// that bounds how far this may move an answer: a firing witness buys back a WHOLE STEP and pays a
/// fraction of ONE ULP.
///
/// # Which steps this is FOR — a measured set, not a rule of thumb
///
/// A step whose own f64 image sits BELOW its decimal is where `round_to_step`'s closing
/// multiplication can land one ULP under the grid point. That condition is NECESSARY (if
/// `fl(step) >= step` then `n·fl(step) >= n·step` and round-to-nearest is monotone) but NOT
/// SUFFICIENT, so the exposed set is MEASURED per rung:
/// `crates/vike-bridge-core/tests/grid_multiple_survives_the_wire.rs` prints each rung's loss count
/// every run, and `crates/vike-bridge-core/tests/format_props.proptest-regressions` carries the
/// `1e-0..=1e-22` sweep with the exact integer comparison that decides each rung's side. ⚠ Do not
/// re-derive that set by reasoning about which powers of ten round which way; it has been got
/// backwards before.
fn grid_multiple_image(value: f64, step: &str) -> Option<Decimal> {
    let step_f = step.parse::<f64>().ok()?;
    if !step_f.is_finite() || step_f <= 0.0 || !value.is_finite() || value == 0.0 {
        return None;
    }
    let n = (value / step_f).round_ties_even();
    // ⚠ `2^52`, not the `2^53` of [`MAX_EXACT_INT`]: see [`MAX_UNAMBIGUOUS_MULTIPLE`].
    if n == 0.0 || n.abs() > MAX_UNAMBIGUOUS_MULTIPLE {
        return None;
    }
    if n * step_f != value {
        return None;
    }
    Decimal::from(n as i64).checked_mul(dec_from_step(step)?)
}

/// Quantize `value` down to `step`'s precision; return a plain decimal string (or the grid
/// multiple `grid_multiple_image` recognises). `step` <= 0 or unparseable → the value as-is, no
/// rounding: submit still attempts the order instead of crashing.
pub fn format_to_step(value: f64, step: &str) -> String {
    let Some(value_d) = dec_from_f64(value) else {
        return py_f64_str(value);
    };
    quantize_to_step(grid_multiple_image(value, step).unwrap_or(value_d), step)
}

/// The shared Decimal tail of [`format_to_step`]: `(value / step).to_integral_value(ROUND_DOWN) *
/// step`, emitted plain — factored out so [`format_scaled_to_step_f`] can feed it an
/// already-rescaled Decimal instead of a (lossy) f64.
fn quantize_to_step(value_d: Decimal, step: &str) -> String {
    let step_d = match dec_from_step(step) {
        Some(s) if s > Decimal::ZERO => s,
        _ => return plain(value_d),
    };
    // (value / step).to_integral_value(ROUND_DOWN) * step
    let Some(ratio) = value_d.checked_div(step_d) else {
        return plain(value_d);
    };
    let mut quantized = ratio.trunc() * step_d; // ROUND_DOWN = toward zero
    // The result scale is the step's scale, unconditionally. rust_decimal does not guarantee that
    // (a zero product loses its scale), so set it: "0" at step "1.00000000" emits "0.00000000".
    quantized.rescale(step_d.scale());
    // NEGATIVE ZERO is kept ("-0.35" at step "1" -> "-0"); rust_decimal normalizes the sign away,
    // so restore it explicitly.
    if quantized.is_zero() && value_d.is_sign_negative() {
        return format!("-{}", plain(quantized));
    }
    plain(quantized)
}

/// Plain positional notation: rust_decimal's Display is already non-exponent and preserves scale
/// zeros.
fn plain(d: Decimal) -> String {
    d.to_string()
}

/// [`format_to_step`] for an `f64` step. The step is spelled through [`py_f64_str`] first, whose
/// trailing ".0" on an integral float carries scale into the Decimal and therefore into the emitted
/// precision.
pub fn format_to_step_f(value: f64, step: f64) -> String {
    format_to_step(value, &py_f64_str(step))
}

/// [`format_to_step_f`]'s exact-rational sibling: quantize `value · num / den` down to `step`.
///
/// The rescale happens IN Decimal, BEFORE the round-down. Pre-scaling in f64 by a non-dyadic
/// rational (1/3 — e.g. a venue-ratio-reduced Deribit combo) can land a value whose exact
/// rational image lies ON the grid one ULP below it, where the truncation then costs a full
/// step: `0.03 · ⅓` at step `0.0005` emits `"0.0095"` through the f64 route but `"0.0100"`
/// here. `num == den` short-circuits to the plain path (the common, unreduced-combo case); a
/// Decimal overflow or `den == 0` falls back to quantizing the f64 product — the
/// lossy-but-sign-correct behavior — so the function stays total.
pub fn format_scaled_to_step_f(value: f64, num: i64, den: i64, step: f64) -> String {
    if num == den && num != 0 {
        return format_to_step_f(value, step);
    }
    let scaled = dec_from_f64(value)
        .and_then(|v| v.checked_mul(Decimal::from(num)))
        .and_then(|p| p.checked_div(Decimal::from(den)));
    match scaled {
        Some(d) => quantize_to_step(d, &py_f64_str(step)),
        None => format_to_step_f(value * num as f64 / den as f64, step),
    }
}

#[cfg(test)]
mod tests {
    //! `format_to_step` itself is golden-gated by `fixtures/r6/format.json`; this module pins only
    //! the exact-rational sibling, whose reason to exist is the one-ULP tick loss.
    use super::*;

    #[test]
    fn identity_rational_matches_the_plain_path() {
        assert_eq!(format_scaled_to_step_f(2.37, 1, 1, 0.1), format_to_step_f(2.37, 0.1));
        assert_eq!(format_scaled_to_step_f(-0.0125, 1, 1, 0.0005), "-0.0125");
    }

    /// THE motivating case: at scale ⅓ the f64 product of an on-grid rational lands one ULP low
    /// and the round-down eats a full tick/step; the Decimal rescale does not.
    #[test]
    fn non_dyadic_rescale_is_exact_where_f64_loses_a_tick() {
        // price direction: 0.03 · (1.0/3.0) = 0.009999999999999998 in f64 → truncates to 0.0095
        assert_eq!(format_to_step_f(0.03 * (1.0 / 3.0), 0.0005), "0.0095", "the pre-fix loss");
        assert_eq!(format_scaled_to_step_f(0.03, 1, 3, 0.0005), "0.0100");
        // qty direction: 2.3 / (1.0/3.0) = 6.899999999999999 in f64 → truncates to 6.8
        assert_eq!(format_to_step_f(2.3 / (1.0 / 3.0), 0.1), "6.8", "the pre-fix loss");
        assert_eq!(format_scaled_to_step_f(2.3, 3, 1, 0.1), "6.9");
    }

    /// The sign rides on either the value or `num` (a credit combo / an inverted orientation) and
    /// truncation stays toward zero.
    #[test]
    fn negative_value_and_negative_num_keep_the_sign() {
        assert_eq!(format_scaled_to_step_f(-0.03, 1, 3, 0.0005), "-0.0100");
        assert_eq!(format_scaled_to_step_f(0.03, -1, 3, 0.0005), "-0.0100");
        assert_eq!(format_scaled_to_step_f(-0.03, -1, 3, 0.0005), "0.0100");
    }

    /// Totality: `den == 0` is unreachable from the combo mapper (zero ratios are rejected
    /// upstream) but must not panic here.
    #[test]
    fn zero_den_falls_back_without_panicking() {
        let s = format_scaled_to_step_f(1.0, 1, 0, 0.1);
        assert!(!s.is_empty());
    }
}
