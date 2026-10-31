//! The one piece of integer arithmetic only the SPOT fine-tick smoke needs: the selection criterion.
//! The exact-decimal pieces it shares with the other venues' fine-tick smokes (`Dec`, `parse_dec`,
//! `decimal_eq`, `steps_off`, `render_multiple`) live in `vike_bridge_core::grid_decimal`.

use vike_bridge_core::grid_decimal::Dec;

/// `x == mantissa · 2^exponent`, EXACTLY, for a finite positive `x`. Plain IEEE-754 field
/// extraction — no approximation anywhere, which is the whole point of using it below.
pub fn decompose(x: f64) -> Option<(u128, i32)> {
    if !(x.is_finite() && x > 0.0) {
        return None;
    }
    let bits = x.to_bits();
    let raw_exponent = ((bits >> 52) & 0x7ff) as i32;
    let fraction = u128::from(bits & 0x000f_ffff_ffff_ffff);
    if raw_exponent == 0 {
        Some((fraction, -1074))
    } else {
        Some((fraction | (1u128 << 52), raw_exponent - 1075))
    }
}

/// `x << shift`, or `None` if that would lose a bit.
pub fn shl_exact(x: u128, shift: u32) -> Option<u128> {
    if shift >= 128 || x.leading_zeros() < shift {
        return None;
    }
    Some(x << shift)
}

/// **THE SELECTION CRITERION.** Does the f64 image of this step sit STRICTLY BELOW the decimal it
/// spells?
///
/// This is the whole reason a symbol is chosen or passed over, so it is COMPUTED rather than
/// looked up in a list of known-bad ticks. The measured exposed set is a MANTISSA family, not a
/// magnitude one — a list of it would be both incomplete and unable to explain itself, while the
/// property is what the defect actually keys on.
///
/// **Why it decides the outcome.** `round_to_step` returns `n as f64 * fl(step)`. When
/// `fl(step) < step` that product is pulled toward the low side of the true grid point `n·step`,
/// and once it lands below that point's own float, its shortest-round-trip decimal is below
/// `n·step` too — which is exactly the input on which the pre-fix `quantize_to_step` truncated a
/// whole step away. When `fl(step) >= step` the product is `>= n·step` in real arithmetic and
/// rounding is monotone, so the result can never fall below the grid point's float and the
/// truncation has nothing to eat.
///
/// ⚠ **This is the NECESSARY half only, and the selection below relies on that being understood.**
/// The monotonicity argument makes a `false` here conclusive — such a tick provably cannot
/// demonstrate the defect, so passing it over costs no coverage. A `true` proves nothing on its
/// own: which multiples of an exposed tick actually drift is measured, not derived, which is why
/// selection goes on to require a multiple inside the resting band that genuinely lands below the
/// grid point's float (`ulps_below`) rather than resting on this verdict alone. The two most
/// common live exposed grids are 1e-6 and 1e-7; the per-rung counts are in
/// `crates/vike-bridge-core/tests/grid_multiple_survives_the_wire.rs`.
///
/// **How it is decided, exactly.** `step_f == mantissa · 2^exponent` and the decimal is
/// `m / 10^k`, both exact, so `step_f < m / 10^k` is the integer question
/// `mantissa · 10^k · 2^exponent < m`, cross-multiplied into `u128` with every operation checked.
///
/// `None` means the comparison overflowed and the answer is UNKNOWN. That is deliberately not
/// folded into `false`: an undecidable step is counted as such in the survey, so a skip report can
/// never quietly claim a grid was examined when part of it was not.
pub fn f64_image_sits_below_its_decimal(step_f: f64, step: Dec) -> Option<bool> {
    let (mantissa, exponent) = decompose(step_f)?;
    let m = u128::try_from(step.digits).ok()?;
    let mut lhs = mantissa.checked_mul(10u128.checked_pow(step.scale)?)?;
    let mut rhs = m;
    if exponent >= 0 {
        lhs = shl_exact(lhs, u32::try_from(exponent).ok()?)?;
    } else {
        rhs = shl_exact(rhs, u32::try_from(-exponent).ok()?)?;
    }
    Some(lhs < rhs)
}
