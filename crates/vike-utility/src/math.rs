//! Numeric helpers two or more crates copied (decision 0141).
//!
//! ⚠ Results here are compared BIT FOR BIT (the options pricing parity fixtures, `p_up`'s cheap_np
//! parity gate, the per-crate libm platform probes), so an expression's operand order and
//! association are part of its contract: a rewrite that is algebraically equal is still a change.
//! Every transcendental comes from the `libm` crate, never from `f64`'s platform-libm methods
//! (`docs/decisions/0032-transcendentals-come-from-the-libm-crate-not-the-platform.md`). The one
//! result nothing compares is [`fmt_number`]'s, a display string.

/// The most decimals [`fmt_number`] will render before giving up and printing the raw value.
const MAX_RENDER_DECIMALS: usize = 12;

/// How far a rendering may sit from the true value, RELATIVE to `max(|v|, 1)`.
///
/// 1e-12 is four orders of magnitude above f64's ~1e-16 multiply noise and far below any difference
/// a person could act on: no order size, price or ceiling in this system is decided at the twelfth
/// significant figure. It is a DISPLAY tolerance and touches nothing else — a guardrail compares the
/// raw `f64`s, so the verdict on the same line is computed from the exact values.
const RENDER_TOLERANCE: f64 = 1e-12;

/// **The one text form of an engine number** (`vike-cli`'s tables and previews, the health rows, the
/// Telegram channel's reads and previews): an `f64` the way a person would have written it, without
/// the binary-floating-point tail.
///
/// `0.4 * 3` is exactly `1.2000000000000002`, and `f64::to_string` is obliged to print all of it —
/// it is the shortest string that round-trips to that bit pattern. So the order preview read
/// `notional=1.2000000000000002`, seventeen digits of arithmetic noise on a number the operator
/// typed as two. That is not a cosmetic problem in a tool whose whole job is letting a human check a
/// number before it becomes an order: a reader who has learned to skip past noise is a reader who
/// will skip past a real discrepancy in the same position.
///
/// The rule is the SHORTEST fixed-point rendering within `RENDER_TOLERANCE` of the value, which is
/// exactly "drop the digits that carry no information": `1.2000000000000002` → `1.2`, while
/// `0.0001234` keeps every digit it has because dropping one would move the value.
///
/// ⚠ **It is never used to decide anything.** Values below ~1e-12 collapse to `0`, and that is
/// acceptable precisely because no comparison reads this string.
///
/// It is not `vike_ui_theme::fmt`, an egui leaf, nor `vike_bridge_core::format`'s `format_to_step`,
/// which rounds DOWN by design ("never overshoot a limit"), the wrong direction here.
pub fn fmt_number(v: f64) -> String {
    if !v.is_finite() {
        return v.to_string();
    }
    let tolerance = RENDER_TOLERANCE * v.abs().max(1.0);
    for decimals in 0..=MAX_RENDER_DECIMALS {
        let rendered = format!("{v:.decimals$}");
        if rendered.parse::<f64>().is_ok_and(|back| (back - v).abs() <= tolerance) {
            return rendered;
        }
    }
    v.to_string()
}

/// The standard-normal CDF `Φ(x) = ½·(1 + erf(x/√2))`, with `erf` from `libm` (pure-Rust FDLIBM,
/// bit-identical to CPython's `math.erf`).
///
/// Saturates to exactly `0.0`/`1.0` in the far tails (`erf` reaches ±1), which the options parity
/// fixtures' deep out-of-the-money prices rely on. Not the `erfc` form `0.5·erfc(−x/√2)`: that one
/// keeps precision in the lower tail and so returns DIFFERENT bits there.
#[inline]
pub fn norm_cdf(x: f64) -> f64 {
    0.5 * (1.0 + libm::erf(x / std::f64::consts::SQRT_2))
}

#[cfg(test)]
#[path = "math_tests.rs"]
mod tests;
