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
/// answer and the witness below declines rather than guessing.
const MAX_EXACT_INT: f64 = 9_007_199_254_740_992.0;

/// ⚠ A WITNESS, not a tolerance. Returns the EXACT decimal `n · step` when `value` is
/// BIT-EXACTLY the f64 image of an integer multiple of `step`; `None` otherwise, and the
/// caller then quantizes the value's own decimal image exactly as before.
fn grid_multiple_image(value: f64, step: &str) -> Option<Decimal> {
    let step_f = step.parse::<f64>().ok()?;
    if !step_f.is_finite() || step_f <= 0.0 || !value.is_finite() || value == 0.0 {
        return None;
    }
    let n = (value / step_f).round_ties_even();
    if n == 0.0 || n.abs() > MAX_EXACT_INT {
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
