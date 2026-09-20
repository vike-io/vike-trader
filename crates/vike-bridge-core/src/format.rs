//! Order qty/price as DECIMAL STRINGS quantized to stepSize/tickSize. Exact port of
//! `exec/binance/format.py` — the PRIMARY pinned Decimal wire site (plan §f64-policy: the core
//! stays f64, and `rust_decimal` appears only where an exact decimal is what goes on the wire).
//!
//! ⚠ **This header used to end "`rust_decimal` exists only here and at OKX `_to_contracts`", and
//! that had stopped being true.** There are THREE Decimal wire sites, not two:
//! `crates/bridges/okx/src/perp.rs`'s `to_contracts` (spelled without the leading underscore the
//! Python original carried), and `crates/bridges/hyperliquid/src/px.rs` — a whole module of them
//! (`float_to_wire` / `clamp_price` / `round_size`), whose own doc has called itself "the twin of
//! `vike_bridge_core::format::format_to_step`" since it was written. The roster is not restated
//! anywhere in prose: `crates/vike-bridge-core/tests/wire_quantizer_probe.rs`'s `WIRE_FILES` is
//! the list, its source gate walks exactly those files, and its hash pin drives the ones it can
//! call. `CLAUDE.md` carries the same stale sentence and has been corrected to point here.
//!
//! RiskGate rounds to tick/lot but leaves IEEE artifacts (0.30000000000000004) that
//! trigger Binance -1111 BAD_PRECISION. Quantize DOWN (never overshoot a limit) to the
//! step's decimal places and emit a plain (non-exponent) string.
//!
//! ⚠ **Quantizing DOWN is also an AMPLIFIER, and the direction the paragraph above names is only
//! half of it.** An overshoot is what the venue rejects; the opposite artifact is silent. The
//! truncation is `(value / step).trunc()`, so a value sitting ONE ULP BELOW a grid point emits a
//! whole step less — the hazard [`format_scaled_to_step_f`] exists for, stated there for the
//! non-dyadic-rational case. The consequence for cross-platform determinism is the reason
//! `wire_quantizer_probe.rs` exists: a last-bit disagreement anywhere upstream of this function
//! (a platform libm, before ADR 0032 converted them) does not stay a last bit here — it becomes a
//! different tick or lot on a live order. That probe's cliff rows measure how far.
//!
//! ⚠ **"Never overshoot" has exactly ONE sanctioned exception, and it was RULED ON rather than
//! merely tolerated**: [`grid_multiple_image`] deliberately emits a decimal fractionally ABOVE the
//! value's own image when that value is bit-exactly the f64 image of a grid point, because the
//! alternative is the whole-step loss the paragraph above describes. Read that function's doc
//! before reasoning from this header's rule; it carries the verdict and the date.

use rust_decimal::prelude::*;

/// Python `str(float)` twin: same shortest-roundtrip digits, but CPython always keeps a
/// decimal point for integral floats ("1.0" where Rust prints "1") — and that trailing
/// zero survives into `Decimal(str(x))`'s scale, visible on the no-step passthrough.
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

/// Quantize `value` down to `step`'s precision; return a plain decimal string.
/// `step` <= 0 or unparseable → the value as-is, no rounding (submit still attempts the
/// order instead of crashing — Python's zero-step guard).
/// The largest f64 that is still an exact integer count — `2^53`. Above it consecutive
/// integers are not representable, so "which multiple of the step is this" has no f64
/// answer at all.
///
/// ⚠ This is NOT where [`grid_multiple_image`] declines any more — it declines at
/// [`MAX_UNAMBIGUOUS_MULTIPLE`], a binade lower, for a reason that constant states. This one
/// survives as the DERIVATION base and as the twin of the constant of the same name in
/// `crates/bridges/hyperliquid/src/px.rs`, whose `grid_multiple_image` is a port of this one.
const MAX_EXACT_INT: f64 = 9_007_199_254_740_992.0;

/// The largest multiple count [`grid_multiple_image`] will answer for — HALF of
/// [`MAX_EXACT_INT`], i.e. `2^52`. The half-binade it gives up is a SECOND defect rather than
/// caution, and the two are opposite in kind: [`MAX_EXACT_INT`] is where an `n` stops being
/// REPRESENTABLE, this is where an `n` stops being UNIQUE.
///
/// ⚠ **`n · step_f == value` is a witness only while it has ONE solution, and above `2^52` it
/// stops having one.** For a `value` in `[2ᵉ, 2ᵉ⁺¹)` the f64 spacing is `2ᵉ⁻⁵²`, so
/// `n = value / step > 2^52` is exactly the condition `step < ulp(value)`: the GRID is finer
/// than the FLOATS at that magnitude, several consecutive multiples of the step round to the
/// same f64, and the bit-exact re-multiplication below passes for EVERY one of them.
/// `round_ties_even(value / step_f)` then returns whichever one its own two roundings land on
/// — not the one the caller is holding — and the witness hands that back as exact. This is not
/// the lost step the witness exists to prevent; it is a CORRECT value corrupted into a wrong
/// one, so the two hazards are argued and tested separately.
///
/// MEASURED in a the CI box lane at the step Alpaca's order body hardcodes
/// (`crates/bridges/alpaca/src/event_mapper.rs`'s `build_order_body`, a 9-decimal precision
/// cap): `format_to_step(9_000_000.0, "0.000000001")` returned `"8999999.999999999"` — a
/// whole-share equity order rendered as a fractional quantity, which that venue rejects on
/// most order types. The exposed band is `value / step ∈ (2^52, 2^53]`, which at a 9-decimal
/// step is `8_388_609..=9_007_199` shares, and **every whole share count in it came back
/// fractional — 618_591 of 618_591, each exactly one step LOW.** (8_388_608 itself is clean:
/// its own multiple count rounds back to the right integer.) The sweep, the counts and the
/// edges are in `crates/vike-bridge-core/tests/grid_witness_is_unambiguous.rs`.
///
/// **What the bound restores is the ROUND-DOWN CONTRACT, and that is a smaller claim than
/// "the cure still works up here" deliberately.** Above `2^52` no formatter can recover the
/// multiple the caller meant, because `crates/vike-model/src/scalar.rs`'s `round_to_step` can
/// no longer land on the grid point either — at those counts its own `n · step_f` product is
/// already one or two ULPs away. MEASURED over 4001 on-grid multiples across the band at step
/// `1e-9`: the witness missed the exact multiple 248 times and the plain truncation 1554,
/// **and BELOW the band, where this change does nothing at all, the same metric already reads
/// 58 and 92** — so that metric is measuring `round_to_step`, not the witness. What IS
/// recoverable is the contract this file opens with: the output must be the round-down of its
/// INPUT. The witness broke that in both directions in the band — emitting a string a whole
/// step below the value (the Alpaca case) and, at other rungs, one above it — and declining
/// restores it by construction.
///
/// The cure this witness exists for lives at an `n` in the low thousands — the sweep in
/// `crates/vike-bridge-core/tests/grid_multiple_survives_the_wire.rs` runs `n ≤ 2000`, twelve
/// orders of magnitude inside this bound, and its every rung is byte-identical across this
/// change.
///
/// ⚠ **This is NOT a direction-safe change and must not be described as one.** MEASURED across
/// the step ladder inside the band, the emitted string moves by exactly one step and the SIGN
/// depends on the rung: at `1e-9` it only ever moves UP (1533 of 4001 rows), at `1e-7` it only
/// ever moves DOWN (1408), at `1e-8` both (445 up, 23 down). Outside the band nothing moves at
/// all.
///
/// ⚠ The hyperliquid twin (`crates/bridges/hyperliquid/src/px.rs`'s `grid_multiple_image`) is
/// still bounded at its own `2^53` and is NOT narrowed here: its exposed band needs a size
/// above `2^52 · 10⁻ˢᶻᵈᵉᶜⁱᵐᵃˡˢ` on a venue whose live `szDecimals` reach 8, and that file is
/// being edited on other branches. The divergence is declared rather than silent.
const MAX_UNAMBIGUOUS_MULTIPLE: f64 = MAX_EXACT_INT / 2.0;

/// ⚠ A WITNESS, not a tolerance. Returns the EXACT decimal `n · step` when `value` is
/// BIT-EXACTLY the f64 image of an integer multiple of `step`; `None` otherwise, and the
/// caller then quantizes the value's own decimal image exactly as before.
///
/// # ⚠ The overshoot is DELIBERATE and was RULED ON — 2026-09-14
///
/// The decimal this returns is, by construction, LARGER in magnitude than the value's own
/// shortest-round-trip image — it has to be, since the value arrived one f64 ULP BELOW the grid
/// point. That puts it in direct conflict with the module header above ("never overshoot a
/// limit"), and the two cannot both hold. **The owner ruled that the wire owes the GRID MULTIPLE
/// the caller asked for, not the value's own image.** This function is the ruling; it is not an
/// unexamined artefact, and removing it to restore the stricter reading would be reopening a
/// settled question rather than tightening a loose one.
///
/// The verdict, the trade it was made on, and the pointer to every measurement behind it live in
/// ONE place and are deliberately not re-spelled here:
/// `crates/vike-bridge-core/tests/format_props.rs`'s `OVERSHOOT_ULP_BUDGET`, which is also the
/// law that pins how far this may move an answer. The shape of the trade, without the numbers: a
/// firing witness buys back a WHOLE STEP and pays a fraction of ONE ULP, and those two are
/// measured to coincide exactly — every firing is an overshoot and every overshoot rescues a full
/// step, with no exceptions.
///
/// # Which steps this is FOR — a measured set, not a rule of thumb
///
/// A step whose own f64 image sits BELOW its decimal is where `round_to_step`'s closing
/// multiplication can land one ULP under the grid point and the truncation below eats a whole
/// step. That condition is NECESSARY and provably so — if `fl(step) >= step` then
/// `n·fl(step) >= n·step` in real arithmetic and round-to-nearest is monotone, so the product
/// cannot fall below the grid point's own float — but it is NOT SUFFICIENT, and how many multiples
/// of an exposed rung actually drift has no closed form. **So the exposed set is MEASURED, per
/// rung, and the measurement is not restated here**:
/// `crates/vike-bridge-core/tests/grid_multiple_survives_the_wire.rs` sweeps the wire ladder on
/// every run and prints the loss count for each rung, and
/// `crates/vike-bridge-core/tests/format_props.proptest-regressions` carries the full
/// `1e-0..=1e-22` sweep together with the exact integer comparison that decides each rung's side.
///
/// ⚠ Do not re-derive that set by reasoning about which powers of ten round which way. It has been
/// got backwards once already, in a 2026-09-14 re-measurement whose sign test was inverted on
/// three rungs; the record above carries both the wrong answer and the arithmetic that settles it.
fn grid_multiple_image(value: f64, step: &str) -> Option<Decimal> {
    let step_f = step.parse::<f64>().ok()?;
    if !step_f.is_finite() || step_f <= 0.0 || !value.is_finite() || value == 0.0 {
        return None;
    }
    let n = (value / step_f).round_ties_even();
    // ⚠ The bound is `2^52`, not the `2^53` of [`MAX_EXACT_INT`], and the missing half-binade
    // is the whole of a separate defect — see [`MAX_UNAMBIGUOUS_MULTIPLE`].
    if n == 0.0 || n.abs() > MAX_UNAMBIGUOUS_MULTIPLE {
        return None;
    }
    if n * step_f != value {
        return None;
    }
    Decimal::from(n as i64).checked_mul(dec_from_step(step)?)
}

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
    // Python decimal multiplication ALWAYS yields exponent(trunc)+exponent(step) =
    // step's exponent (trunc is scale 0), and the quantize(step) branch lands on the
    // same scale — so the result scale is step_d.scale() unconditionally. rust_decimal
    // does not guarantee that (a zero product loses its scale), so set it explicitly:
    // "0" quantized to step "1.00000000" must emit "0.00000000" like Python.
    quantized.rescale(step_d.scale());
    // Python Decimal keeps NEGATIVE ZERO through trunc/mult ("-0.35" step "1" ->
    // "-0.00000000"); rust_decimal normalizes it away — restore the sign explicitly.
    if quantized.is_zero() && value_d.is_sign_negative() {
        return format!("-{}", plain(quantized));
    }
    plain(quantized)
}

/// Python `format(d, "f")`: plain positional notation (rust_decimal Display is already
/// non-exponent and preserves scale zeros).
fn plain(d: Decimal) -> String {
    d.to_string()
}

/// The float-typed twin (Python accepts `step: str | float`; exec wiring passes the
/// FLOAT from parse_symbol_properties, so the string form goes through `str(float)` first —
/// including Python's trailing ".0" on integral floats, which carries scale into the
/// Decimal and therefore into the emitted precision).
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
    //! `format_to_step` itself is golden-gated elsewhere (r-fixtures); this module pins only the
    //! exact-rational sibling, whose reason to exist is the one-ULP tick loss.
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
