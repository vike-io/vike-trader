//! The one text form of an engine number (decision 0141): [`format_number`]. Its result is a display
//! string nothing compares; the formulas that once shared this module moved to `vike-math` (decision
//! 0294).

/// The most decimals [`format_number`] will render before giving up and printing the raw value.
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
pub fn format_number(v: f64) -> String {
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

#[cfg(test)]
#[path = "number_text_tests.rs"]
mod tests;
